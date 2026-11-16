//! Configuration: provider/base_url/model/api_key, loaded from
//! ~/config/settings/haikucode/config.json and overridable by env. Compatible with the
//! previous Python haikucode config so existing keys/memory carry over.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One MCP server entry: config.json's `mcp_servers`, or a project's
/// `.mcp.json` (Claude Code's `mcpServers` shape). A `command` is spawned over
/// stdio; a `url` is spoken to over Streamable HTTP. `${VAR}` / `${VAR:-x}`
/// in any string field are expanded from the environment at launch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// "stdio" or "http" ("streamable-http"); inferred from command/url when empty.
    #[serde(default, rename = "type", alias = "transport", skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Static HTTP headers (e.g. `Authorization: Bearer ${TOKEN}`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A pre-registered OAuth client, for servers without dynamic registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpOAuthConfig>,
}

impl McpServerConfig {
    /// One `mcp_servers` entry, or None if it's malformed — including one with
    /// neither a `command` nor a `url` (a typo like `"cmd"` parses otherwise).
    pub fn parse(val: &serde_json::Value) -> Option<McpServerConfig> {
        serde_json::from_value::<McpServerConfig>(val.clone())
            .ok()
            .filter(|c| !c.command.is_empty() || !c.url.is_empty())
    }
}

/// OAuth client settings for one HTTP MCP server (all optional).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpOAuthConfig {
    #[serde(default, alias = "clientId", skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    #[serde(default, alias = "clientSecret", skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    /// Fixed localhost port for the redirect (0 = any free port).
    #[serde(default, alias = "callbackPort")]
    pub callback_port: u16,
}

/// A subscription-login token set for one provider (see auth.rs). Persisted to
/// config.json (0600) under `oauth`. `expires_at` is unix seconds; 0 = unknown.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: u64,
    /// Provider-specific routing hint (e.g. ChatGPT account id). Optional.
    #[serde(default)]
    pub account_id: String,
}

impl OAuthToken {
    /// True when the access token is missing or within 60s of expiry.
    pub fn needs_refresh(&self) -> bool {
        if self.access_token.is_empty() {
            return true;
        }
        self.expires_at != 0 && now_unix() + 60 >= self.expires_at
    }
}

/// Current unix time in seconds (0 if the clock is before the epoch).
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    /// Effective API key for the current provider (resolved from api_keys or env;
    /// NOT persisted directly — api_keys is the persistent store).
    #[serde(default)]
    pub api_key: String,
    /// Per-provider API keys persisted to disk. Keyed by provider name.
    #[serde(default)]
    pub api_keys: BTreeMap<String, String>,
    /// Per-provider subscription-login tokens (OAuth). Keyed by provider name.
    #[serde(default)]
    pub oauth: BTreeMap<String, OAuthToken>,
    /// Credential source for the active provider (mirrors its `auth_modes`
    /// entry; see `AuthMethod`). Not persisted itself.
    #[serde(default = "default_auth_mode")]
    pub auth_mode: String,
    /// How each provider signs in, by name: "api" (API key, the default),
    /// "sub" (subscription login), "plan" (a coding-plan key on the plan's
    /// own endpoint) or "none" (a keyless custom server).
    #[serde(default)]
    pub auth_modes: BTreeMap<String, String>,
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Model context window, for the ctx usage bar.
    #[serde(default = "default_ctx")]
    pub context_window: u32,
    /// True when the user pinned context_window themselves (config.json key or
    /// the `/config` panel). When false the window is auto-derived from the
    /// model, so switching models picks up the right size; a user value is
    /// never overwritten or rewritten to disk by auto-detection.
    #[serde(skip)]
    pub context_window_explicit: bool,
    /// Per-model overrides (`models` in config.json), keyed by model id:
    /// context window, temperature and prices. The active model's entry wins
    /// over the global and per-provider values, so one config can mix a 128k
    /// local server, an 8k one and a 1M hosted model, each priced right.
    #[serde(default)]
    pub models: BTreeMap<String, ModelSettings>,
    /// USD per 1M tokens, for the session cost readout. Mirrors the active
    /// provider's price list (re-derived on load and every provider switch);
    /// the per-provider store is `prices`, with `PRICES` as the built-in
    /// fallback (DeepSeek defaults).
    #[serde(default = "default_price_in")]
    pub price_in: f64,
    #[serde(default = "default_price_out")]
    pub price_out: f64,
    /// Currency `price_in`/`price_out` are quoted in (ISO 4217). DeepSeek bills
    /// China-region accounts in CNY while its published price lists are USD, so
    /// this exists to keep the cost readout in the same units as the account
    /// balance: set it to `CNY` together with the CNY price list.
    #[serde(default = "default_price_currency")]
    pub price_currency: String,
    /// MCP servers to launch, keyed by name (advertised as mcp__<name>__<tool>).
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    /// Auto-commit each successful edit to the working-directory git repo so
    /// every change is a restorable checkpoint. On by default; no-op outside a repo.
    #[serde(default = "default_true")]
    pub auto_commit: bool,
    /// Show the model's full reasoning, tool output and diffs in the
    /// transcript. Off (the default), thoughts stay as list entries and tool
    /// activity appears only as a live status; Ctrl+T or `/thinking` flips it.
    #[serde(default)]
    pub show_thinking: bool,
    /// The first status row's segments, in order (`/statusline`). Empty means
    /// the default row; see `ui::statusline`.
    #[serde(default)]
    pub statusline: Vec<String>,
    /// Look for a newer GitHub release at launch (at most daily). `/update`
    /// installs it either way.
    #[serde(default = "default_true")]
    pub update_check: bool,
    /// Permission mode new sessions start in: "ask", "yolo", or "plan" ("bypass"
    /// and "auto" still load as yolo). Saved whenever the mode is switched.
    #[serde(default = "default_permission")]
    pub permission: String,
    /// Max tool-call rounds per turn. 0 means "auto" (an internal safe limit).
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: u32,
    /// Sampling temperature. Unset, haikucode sends none and the model's own
    /// recommended sampling applies (see `temperature()`).
    #[serde(default)]
    pub temperature: Option<f64>,
    /// How hard the model should reason: "auto" (send nothing; the model's own
    /// default), "off", "low", "medium", "high", "xhigh" or "max". Mapped per provider in
    /// `api::reasoning_fields` / the native adapters. Replaces the old
    /// DeepSeek-only `thinking` bool, which still loads (true → "medium").
    #[serde(default = "default_reasoning")]
    pub reasoning: String,
    /// Wire protocol per provider (`apis` in config.json): "chat" (OpenAI
    /// chat/completions), "messages" (Anthropic) or "responses" (OpenAI
    /// Responses). Unset, the provider decides (see `api_kind`). Per provider
    /// so a protocol set for one gateway doesn't follow a switch to a local
    /// server; a legacy top-level `api` belongs to the provider active when
    /// it was written.
    #[serde(default)]
    pub apis: BTreeMap<String, String>,
    /// Price per 1M cached (cache-hit) input tokens. Unset: a tenth of
    /// `price_in`, the usual cache-read discount (see `PriceList::price_cached`).
    #[serde(default)]
    pub price_cached_in: Option<f64>,
    /// True when the current provider's key came from the environment; we never
    /// persist it then.
    #[serde(skip)]
    pub key_from_env: bool,
    /// Providers the user added (any OpenAI-compatible endpoint), switchable
    /// like the presets. Their keys live in `api_keys` under their name.
    #[serde(default)]
    pub custom_providers: Vec<CustomProvider>,
    /// User-set price lists, per provider (the cost counter follows the active
    /// provider). Built-ins fall back to `PRICES`; a custom provider without
    /// one has no known cost and its readout is hidden.
    #[serde(default)]
    pub prices: BTreeMap<String, PriceList>,
    /// Settings blocks haikucode doesn't model as fields (`permissions`, `hooks`)
    /// carried through verbatim so a model/theme rewrite can't drop them.
    #[serde(skip)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A user-added provider: a name, an OpenAI-compatible base URL and the model
/// to use there (kept up to date as the model changes while it's active).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomProvider {
    pub name: String,
    pub base_url: String,
    pub model: String,
}

/// One model's overrides in `models` (config.json). Every field is optional;
/// unset ones fall through to the global / per-provider value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Prices per 1M tokens; set on top of the provider's list, so a model
    /// can override just its input/output price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_in: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_out: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_cached_in: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_currency: Option<String>,
}

impl ModelSettings {
    fn is_empty(&self) -> bool {
        *self == ModelSettings::default()
    }
    fn has_price(&self) -> bool {
        self.price_in.is_some() || self.price_out.is_some()
    }
}

/// One provider's price list, per 1M tokens. Stored per provider (the
/// `prices` map in config.json) so the cost counter follows the active
/// provider; the built-in `PRICES` table covers the presets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PriceList {
    pub price_in: f64,
    pub price_out: f64,
    /// Price per 1M cached (cache-hit) input tokens. Unset: a tenth of
    /// `price_in`, the usual cache-read discount (see `price_cached`).
    #[serde(default)]
    pub price_cached_in: Option<f64>,
    /// ISO code the prices are quoted in.
    #[serde(default = "default_price_currency")]
    pub price_currency: String,
}

impl Default for PriceList {
    fn default() -> Self {
        PriceList {
            price_in: 0.0,
            price_out: 0.0,
            price_cached_in: None,
            price_currency: default_price_currency(),
        }
    }
}

impl PriceList {
    /// Price per 1M cached input tokens: the configured one, else a tenth of
    /// `price_in`, the usual cache-read discount (Anthropic, OpenAI).
    pub fn price_cached(&self) -> f64 {
        self.price_cached_in.unwrap_or(self.price_in * 0.1)
    }
}

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

fn default_true() -> bool {
    true
}

fn default_permission() -> String {
    "ask".to_string()
}
fn default_auth_mode() -> String {
    "api".to_string()
}
fn default_max_tool_calls() -> u32 {
    0 // "auto" — see AUTO_STEPS in agent.rs
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
fn key_env_vars(provider: &str) -> &'static [&'static str] {
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

fn default_theme() -> String {
    "HaikuOS".to_string()
}
fn default_ctx() -> u32 {
    // Matches the default model, DeepSeek v4 Flash (1M-token context window).
    1_000_000
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
// DeepSeek Flash (`deepseek-flash`, V4.1) off-peak list, USD per 1M tokens.
// Since 2026-08-16 DeepSeek charges double during peak hours (01:00-04:00 and
// 06:00-10:00 UTC, weekdays); the flat readout uses the off-peak price, which
// covers about four in five hours.
fn default_price_in() -> f64 {
    0.15
}
fn default_price_out() -> f64 {
    0.6
}
/// DeepSeek Flash's off-peak cache-hit input price (USD per 1M), next to the
/// default 0.15 miss / 0.6 output list.
const DEEPSEEK_CACHE_HIT_PRICE: f64 = 0.003;

/// The reasoning settings, in `/config` cycling order.
/// `xhigh` and `max` go out as-is where the provider has them (current Claude
/// models, newer OpenAI GPT-5 models, OpenRouter) and as `high` elsewhere.
pub const REASONING_LEVELS: [&str; 7] = ["auto", "off", "low", "medium", "high", "xhigh", "max"];

fn default_reasoning() -> String {
    "auto".to_string()
}

/// A reasoning setting from config or `/effort`: one of `REASONING_LEVELS`
/// (with "none"/"false" read as off and "on"/"true" as medium), else auto.
pub fn normalize_reasoning(r: &str) -> String {
    let r = r.trim().to_ascii_lowercase();
    match r.as_str() {
        "none" | "false" | "disabled" => "off".into(),
        "on" | "true" | "enabled" => "medium".into(),
        _ if REASONING_LEVELS.contains(&r.as_str()) => r,
        _ => default_reasoning(),
    }
}

/// The wire protocol a request goes out on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKind {
    /// OpenAI chat/completions — every OpenAI-compatible server.
    Chat,
    /// Anthropic Messages.
    Messages,
    /// OpenAI Responses.
    Responses,
}

/// The built-in prices are DeepSeek's USD list, so the default unit is USD.
fn default_price_currency() -> String {
    "USD".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
            api_key: String::new(),
            api_keys: BTreeMap::new(),
            oauth: BTreeMap::new(),
            auth_mode: default_auth_mode(),
            auth_modes: BTreeMap::new(),
            theme: default_theme(),
            context_window: default_ctx(),
            context_window_explicit: false,
            models: BTreeMap::new(),
            price_in: default_price_in(),
            price_out: default_price_out(),
            price_currency: default_price_currency(),
            mcp_servers: BTreeMap::new(),
            auto_commit: true,
            show_thinking: false,
            statusline: Vec::new(),
            update_check: true,
            permission: default_permission(),
            max_tool_calls: default_max_tool_calls(),
            temperature: None,
            reasoning: default_reasoning(),
            apis: BTreeMap::new(),
            price_cached_in: None,
            key_from_env: false,
            custom_providers: Vec::new(),
            prices: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// Haiku keeps per-user settings in `~/config/settings` (B_USER_SETTINGS_DIRECTORY);
/// other hosts (the dev Mac, CI) use the XDG-style `~/.config`.
#[cfg(target_os = "haiku")]
const CONFIG_SUBDIR: &str = "config/settings/haikucode";
#[cfg(not(target_os = "haiku"))]
const CONFIG_SUBDIR: &str = ".config/haikucode";

pub fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(CONFIG_SUBDIR)
}

/// On first run on a host that had picoder (which Haiku Code replaces), copy
/// its settings, memory, history, sessions, skills and commands from
/// `~/.config/picoder` so nothing has to be set up again. Leaves picoder's
/// directory as it was; does nothing once `config_dir()` exists. Returns a
/// line saying what happened.
pub fn adopt_picoder() -> Option<String> {
    if cfg!(target_os = "haiku") || cfg!(test) {
        return None;
    }
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    adopt_picoder_from(&home.join(".config/picoder"), &config_dir())
}

fn adopt_picoder_from(old: &Path, new: &Path) -> Option<String> {
    if new.exists() || !old.join("config.json").is_file() {
        return None;
    }
    fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let (src, dst) = (e.path(), to.join(e.file_name()));
            let ty = e.file_type()?;
            if ty.is_dir() {
                // `run/` holds a live picoder's sockets and locks.
                if e.file_name() != "run" {
                    copy_tree(&src, &dst)?;
                }
            } else if ty.is_file() {
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }
    match copy_tree(old, new) {
        Ok(()) => Some(format!("imported your picoder settings, memory and sessions from {}", old.display())),
        Err(e) => {
            // Half a copy would hide the rest next launch: start clean instead.
            let _ = std::fs::remove_dir_all(new);
            Some(format!("couldn't import picoder's settings from {} ({e})", old.display()))
        }
    }
}

/// `config_dir()` as shown to people and the model, e.g. `~/config/settings/haikucode/`.
pub fn config_dir_label() -> String {
    format!("~/{CONFIG_SUBDIR}/")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn memory_path() -> PathBuf {
    config_dir().join("memory.md")
}

/// Write `data` to `path` atomically: write a sibling temp file, fsync it, then
/// rename over the target. A power loss mid-write (common on a Pi running off an
/// SD card) then leaves either the intact old file or the complete new one —
/// never the truncated mix a bare `fs::write` can produce. The temp name is
/// per-process so two haikucode instances don't clobber each other's temp file.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        // Keep the target's mode (config.json is 0600 — it holds API keys):
        // a fresh temp file would otherwise come up 0644 and the rename would
        // silently widen the permissions.
        if let Ok(meta) = std::fs::metadata(path) {
            let _ = f.set_permissions(meta.permissions());
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Every settings document that can carry `permissions` / `hooks`, lowest
/// priority first: the user's config.json, then the project's
/// `.haikucode/settings.json` and `.haikucode/settings.local.json` (the latter is
/// where "don't ask again" rules land, and belongs in .gitignore). Missing or
/// malformed files are skipped. Each entry is (path, parsed JSON object).
pub fn settings_layers() -> Vec<(PathBuf, serde_json::Value)> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let paths = [
        config_path(),
        cwd.join(".haikucode").join("settings.json"),
        cwd.join(".haikucode").join("settings.local.json"),
    ];
    paths
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            v.is_object().then_some((p, v))
        })
        .collect()
}

/// The value of `key` in the most specific settings layer that sets it
/// (project local > project > config.json).
pub fn setting(key: &str) -> Option<serde_json::Value> {
    settings_layers().into_iter().rev().find_map(|(_, v)| v.get(key).cloned())
}

pub fn history_path() -> PathBuf {
    config_dir().join("history")
}

pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

impl Config {
    /// Read config.json leniently: each known field is merged independently,
    /// so one malformed field (a typo in mcp_servers, say) can't reset the
    /// others. No env override — this is the on-disk truth, safe to rewrite.
    /// Read every key haikucode understands out of one config.json document.
    fn absorb_disk(&mut self, v: &serde_json::Value) {
        let cfg = self;
        if let Some(s) = v.get("provider").and_then(|x| x.as_str()) {
            cfg.provider = s.to_string();
        }
        if let Some(s) = v.get("base_url").and_then(|x| x.as_str()) {
            cfg.base_url = s.to_string();
        }
        if let Some(s) = v.get("model").and_then(|x| x.as_str()) {
            cfg.model = s.to_string();
        }
        // New-style per-provider keys (api_keys object).
        if let Some(map) = v.get("api_keys").and_then(|x| x.as_object()) {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    cfg.api_keys.insert(k.clone(), s.to_string());
                }
            }
        }
        // Subscription-login tokens (one per provider).
        // One malformed entry must not take its siblings with it: parse
        // entry by entry, and keep the raw block in `extra` so a later
        // model/theme rewrite preserves the broken entry verbatim
        // instead of dropping the whole map from disk.
        if let Some(m) = v.get("oauth") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match serde_json::from_value::<OAuthToken>(val.clone()) {
                        Ok(t) => {
                            cfg.oauth.insert(k.clone(), t);
                        }
                        Err(_) => broken = true,
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("oauth".into(), m.clone());
            }
        }
        // Legacy single api_key field — use as fallback for the current
        // provider if no per-provider entry exists.
        if let Some(s) = v.get("api_key").and_then(|x| x.as_str()) {
            if !cfg.api_keys.contains_key(&cfg.provider) {
                cfg.api_keys.insert(cfg.provider.clone(), s.to_string());
            }
        }
        if let Some(s) = v.get("theme").and_then(|x| x.as_str()) {
            let s = s.to_string();
            // Validate; unknown theme names silently fall back to default
            // so a hand-edited config can't cause a broken UI.
            cfg.theme = if crate::ui::is_theme_name(&s) { s } else { default_theme() };
        }
        if let Some(n) = v.get("context_window").and_then(|x| x.as_u64()) {
            cfg.context_window = n as u32;
            cfg.context_window_explicit = true;
        }
        // Per-model overrides; one malformed entry is skipped on its own.
        if let Some(m) = v.get("models") {
            match m.as_object() {
                Some(obj) => {
                    for (k, val) in obj {
                        match serde_json::from_value::<ModelSettings>(val.clone()) {
                            Ok(mut ms) => {
                                ms.context_window = ms.context_window.filter(|n| *n > 0);
                                if !ms.is_empty() {
                                    cfg.models.insert(k.clone(), ms);
                                }
                            }
                            Err(e) => eprintln!("config: models[{k:?}] is malformed ({e}) and was skipped"),
                        }
                    }
                }
                None => eprintln!("config: models is not an object and was ignored"),
            }
        }
        // Legacy (pre-`models`): a bare per-model context-window map.
        if let Some(obj) = v.get("model_context_windows").and_then(|m| m.as_object()) {
            for (k, val) in obj {
                if let Some(n) = val.as_u64().filter(|n| *n > 0) {
                    let e = cfg.models.entry(k.clone()).or_default();
                    e.context_window.get_or_insert(n.min(u32::MAX as u64) as u32);
                }
            }
        }
        if let Some(n) = v.get("price_in").and_then(|x| x.as_f64()) {
            cfg.price_in = n;
        }
        if let Some(n) = v.get("price_out").and_then(|x| x.as_f64()) {
            cfg.price_out = n;
        }
        if let Some(s) = v.get("price_currency").and_then(|x| x.as_str()) {
            let s = s.trim().to_ascii_uppercase();
            // An empty or junk value keeps the documented default rather
            // than producing a unitless (or bogus-symbol) readout.
            cfg.price_currency = if s.is_empty() {
                default_price_currency()
            } else {
                s
            };
        }
        if let Some(m) = v.get("mcp_servers") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match McpServerConfig::parse(val) {
                        Some(c) => {
                            cfg.mcp_servers.insert(k.clone(), c);
                        }
                        None => {
                            eprintln!("config: mcp_servers.{k} is malformed and was skipped");
                            broken = true;
                        }
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("mcp_servers".into(), m.clone());
            }
        }
        if let Some(b) = v.get("update_check").and_then(|x| x.as_bool()) {
            cfg.update_check = b;
        }
        if let Some(b) = v.get("show_thinking").and_then(|x| x.as_bool()) {
            cfg.show_thinking = b;
        }
        if let Some(b) = v.get("auto_commit").and_then(|x| x.as_bool()) {
            cfg.auto_commit = b;
        }
        if let Some(list) = v.get("statusline").and_then(|x| x.as_array()) {
            cfg.statusline = list.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
        }
        // The old `thinking` switch only ever reached DeepSeek and GLM
        // (as their `thinking` field); every other provider ignored it.
        // Keep that: "medium" there, nothing sent elsewhere — mapping it
        // to "medium" for a local Qwen would switch reasoning on that
        // the user never actually had.
        if let Some(true) = v.get("thinking").and_then(|x| x.as_bool()) {
            if matches!(cfg.provider.as_str(), "deepseek" | "zai") {
                cfg.reasoning = "medium".into();
            }
        }
        if let Some(s) = v.get("permission").and_then(|x| x.as_str()) {
            cfg.permission = s.to_string();
        }
        // The old global `auth_mode` is deliberately not read: it predates
        // per-provider sign-in, and a stale "sub" would follow every provider.
        if let Some(map) = v.get("auth_modes").and_then(|x| x.as_object()) {
            for (k, val) in map {
                if let Some(s) = val.as_str() {
                    cfg.auth_modes.insert(k.clone(), s.to_string());
                }
            }
        }
        if let Some(n) = v.get("max_tool_calls").and_then(|x| x.as_u64()) {
            cfg.max_tool_calls = n as u32;
        }
        if let Some(t) = v.get("temperature").and_then(|x| x.as_f64()) {
            cfg.temperature = Some(t);
        }
        if let Some(r) = v.get("reasoning").and_then(|x| x.as_str()) {
            cfg.reasoning = normalize_reasoning(r);
        }
        if let Some(obj) = v.get("apis").and_then(|x| x.as_object()) {
            for (k, a) in obj {
                if let Some(a) = a.as_str().map(str::trim).filter(|a| !a.is_empty()) {
                    cfg.apis.insert(k.clone(), a.to_string());
                }
            }
        }
        // Legacy: one top-level `api`, which belonged to the provider active
        // when it was written.
        if let Some(a) = v.get("api").and_then(|x| x.as_str()).map(str::trim).filter(|a| !a.is_empty()) {
            let provider = cfg.provider.clone();
            cfg.apis.entry(provider).or_insert_with(|| a.to_string());
        }
        if let Some(p) = v.get("price_cached_in").and_then(|x| x.as_f64()) {
            cfg.price_cached_in = Some(p);
        }
        // Per-provider price lists (the cost counter follows the provider).
        // One malformed entry must not take its siblings with it; the raw
        // block goes to `extra` so a rewrite keeps it verbatim.
        if let Some(m) = v.get("prices") {
            let mut broken = false;
            if let Some(obj) = m.as_object() {
                for (k, val) in obj {
                    match serde_json::from_value::<PriceList>(val.clone()) {
                        Ok(p) => {
                            cfg.prices.insert(k.clone(), p);
                        }
                        Err(_) => broken = true,
                    }
                }
            } else {
                broken = true;
            }
            if broken {
                cfg.extra.insert("prices".into(), m.clone());
            }
        }
        if let Some(list) = v.get("custom_providers").and_then(|x| x.as_array()) {
            for item in list {
                match serde_json::from_value::<CustomProvider>(item.clone()) {
                    Ok(cp) if valid_custom_name(&cp.name).is_ok() && cp.base_url.starts_with("http") => {
                        cfg.custom_providers.retain(|c| c.name != cp.name);
                        cfg.custom_providers.push(cp);
                    }
                    _ => eprintln!("config: a custom_providers entry is malformed and was skipped"),
                }
            }
        }
        cfg.adopt_active_custom();
        // Legacy: the single top-level price list belonged to the provider
        // active when it was written. Fold it into the per-provider map so
        // switching providers and back keeps it.
        if !cfg.prices.contains_key(&cfg.provider)
            && (cfg.price_in != default_price_in()
                || cfg.price_out != default_price_out()
                || cfg.price_currency != default_price_currency()
                || cfg.price_cached_in.is_some())
        {
            let mut list = PriceList {
                price_in: cfg.price_in,
                price_out: cfg.price_out,
                price_cached_in: cfg.price_cached_in,
                price_currency: cfg.price_currency.clone(),
            };
            // Legacy files predate the per-provider cache price: keep the
            // built-in one (e.g. DeepSeek's 0.003 cache-hit price).
            if list.price_cached_in.is_none() {
                list.price_cached_in = built_in_prices(&cfg.provider).and_then(|b| b.price_cached_in);
            }
            cfg.prices.insert(cfg.provider.clone(), list);
        }
        for key in [
            "permissions",
            "hooks",
            "auto_bump_version",
            "shared_skills",
            "sandbox",
            "sandbox_network",
            "sandbox_allow_write",
            "web_search",
            "web_search_url",
            "web_search_key",
        ] {
            if let Some(x) = v.get(key) {
                cfg.extra.insert(key.to_string(), x.clone());
            }
        }
    }

    fn load_disk() -> Config {
        let mut cfg = Config::default();
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                cfg.absorb_disk(&v);
            }
        }
        // Resolve the effective key from the per-provider map.
        cfg.resolve_key();
        cfg.sync_auth_mode();
        // The top-level price fields mirror the active provider's list.
        cfg.sync_prices();
        cfg
    }

    pub fn load() -> Config {
        let mut cfg = Self::load_disk();
        // First match wins, so HAIKUCODE_API_KEY (listed first) overrides the
        // provider-specific var.
        for var in key_env_vars(&cfg.provider) {
            if let Ok(k) = std::env::var(var) {
                if !k.is_empty() {
                    cfg.api_key = k;
                    cfg.key_from_env = true;
                    break;
                }
            }
        }
        // When the user hasn't pinned a window, derive it from the model so the
        // ctx bar and compaction limit match the model's real capacity rather
        // than a stale 128k default. The worker refines this from the provider's
        // /models metadata (for providers that advertise it) once it's running.
        if !cfg.context_window_explicit {
            cfg.context_window = known_context_window(&cfg.model);
        }
        cfg
    }

    /// Persist to disk with 0600 perms. An env-injected key is never written.
    pub fn save(&self) -> Result<()> {
        // UI tests flip the theme, permission mode and brief view, each of
        // which saves; never let them rewrite the real ~/config file.
        if cfg!(test) {
            return Ok(());
        }
        let dir = config_dir();
        std::fs::create_dir_all(&dir).context("create config dir")?;
        let json = self.to_disk_json();
        let path = config_path();
        atomic_write(&path, serde_json::to_string_pretty(&json)?.as_bytes())
            .context("write config")?;
        set_private(&path);
        Ok(())
    }

    /// The exact JSON `save` writes: the core fields plus every user
    /// customization that differs from its default, so a model/theme rewrite
    /// can't drop the rest of the file. Split out from `save` so the
    /// preserve-what-the-user-set rules are testable without touching disk.
    fn to_disk_json(&self) -> serde_json::Value {
        let mut on_disk = self.clone();
        // Update the per-provider map with the current effective key (unless env-sourced).
        if !self.key_from_env && !self.api_key.is_empty() {
            on_disk.api_keys.insert(self.provider.clone(), self.api_key.clone());
        }
        let mut json = serde_json::json!({
            "provider": on_disk.provider,
            "base_url": on_disk.base_url,
            "model": on_disk.model,
            "theme": on_disk.theme,
        });
        // Write per-provider keys (the canonical store). Also write the legacy
        // api_key field with the current provider's key for older haikucode readers.
        if !on_disk.api_keys.is_empty() {
            json["api_keys"] = serde_json::to_value(&on_disk.api_keys).unwrap_or_default();
            if let Some(k) = on_disk.api_keys.get(&on_disk.provider) {
                if !self.key_from_env {
                    json["api_key"] = serde_json::json!(k);
                }
            }
        }
        // Subscription-login tokens, when any provider has been logged in.
        if !on_disk.oauth.is_empty() {
            json["oauth"] = serde_json::to_value(&on_disk.oauth).unwrap_or_default();
        }
        // Preserve user customizations across model/theme rewrites; only
        // non-default values are written, keeping the common file minimal.
        if !on_disk.mcp_servers.is_empty() {
            json["mcp_servers"] = serde_json::to_value(&on_disk.mcp_servers).unwrap_or_default();
        }
        if !on_disk.update_check {
            json["update_check"] = serde_json::json!(false);
        }
        if on_disk.show_thinking {
            json["show_thinking"] = serde_json::json!(true);
        }
        if !on_disk.statusline.is_empty() {
            json["statusline"] = serde_json::json!(on_disk.statusline);
        }
        if !on_disk.auto_commit {
            json["auto_commit"] = serde_json::json!(false);
        }
        // Only persist a window the user pinned; an auto-derived value stays out
        // of the file so it keeps tracking the model on later launches.
        if on_disk.context_window_explicit {
            json["context_window"] = serde_json::json!(on_disk.context_window);
        }
        let models: BTreeMap<&String, &ModelSettings> = on_disk.models.iter().filter(|(_, m)| !m.is_empty()).collect();
        if !models.is_empty() {
            json["models"] = serde_json::to_value(&models).unwrap_or_default();
        }
        // The top-level price fields mirror the active provider's list (not a
        // model override, which lives in `models` and would otherwise be
        // folded into the provider's list on the next load). Skip them when
        // the provider has no list (a custom server with a hidden cost) so a
        // reload doesn't mistake the zeros for a user price list.
        if let Some(p) = on_disk.price_list() {
            if p.price_in != default_price_in() {
                json["price_in"] = serde_json::json!(p.price_in);
            }
            if p.price_out != default_price_out() {
                json["price_out"] = serde_json::json!(p.price_out);
            }
            if p.price_currency.to_ascii_uppercase() != default_price_currency() {
                json["price_currency"] = serde_json::json!(p.price_currency);
            }
            if let Some(c) = p.price_cached_in {
                json["price_cached_in"] = serde_json::json!(c);
            }
        }
        if !on_disk.prices.is_empty() {
            json["prices"] = serde_json::to_value(&on_disk.prices).unwrap_or_default();
        }
        if on_disk.permission != default_permission() {
            json["permission"] = serde_json::json!(on_disk.permission);
        }
        if !on_disk.auth_modes.is_empty() {
            json["auth_modes"] = serde_json::to_value(&on_disk.auth_modes).unwrap_or_default();
        }
        if on_disk.max_tool_calls != default_max_tool_calls() {
            json["max_tool_calls"] = serde_json::json!(on_disk.max_tool_calls);
        }
        if let Some(t) = on_disk.temperature {
            json["temperature"] = serde_json::json!(t);
        }
        if on_disk.reasoning != default_reasoning() {
            json["reasoning"] = serde_json::json!(on_disk.reasoning);
        }
        if !on_disk.apis.is_empty() {
            json["apis"] = serde_json::to_value(&on_disk.apis).unwrap_or_default();
        }
        if !on_disk.custom_providers.is_empty() {
            json["custom_providers"] = serde_json::to_value(&on_disk.custom_providers).unwrap_or_default();
        }
        for (k, v) in &on_disk.extra {
            json[k.as_str()] = v.clone();
        }
        json
    }

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
            .map(|(n, b, m)| (n.to_string(), b.to_string(), m.to_string()))
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
    fn adopt_active_custom(&mut self) {
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

    fn model_settings_key(&self) -> Option<&String> {
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
        let key = self.model_settings_key().filter(|k| self.models[*k].context_window.is_some()).cloned();
        if let Some(k) = key {
            self.models.entry(k).or_default().context_window = Some(n);
        } else {
            self.context_window = n;
            self.context_window_explicit = true;
        }
    }

    pub fn sync_active_custom(&mut self) {
        let (base_url, model) = (self.base_url.clone(), self.model.clone());
        if let Some(c) = self.custom_providers.iter_mut().find(|c| c.name == self.provider) {
            c.base_url = base_url;
            c.model = model;
        }
    }

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

    /// Persist only the show-thinking toggle, preserving the rest of the file.
    pub fn persist_show_thinking(on: bool) {
        let mut disk = Config::load_disk();
        disk.show_thinking = on;
        let _ = disk.save();
    }

    /// Persist only the status-row segments (empty = default), preserving the
    /// rest of the file.
    pub fn persist_statusline(segments: &[String]) {
        let mut disk = Config::load_disk();
        disk.statusline = segments.to_vec();
        let _ = disk.save();
    }

    /// Persist only the theme name, preserving the rest of the file.
    pub fn persist_theme(theme: &str) {
        let mut disk = Config::load_disk();
        disk.theme = theme.to_string();
        let _ = disk.save();
    }

    /// Resolve the effective API key for the current provider: its saved key,
    /// else its environment variable (so a provider whose key lives only in
    /// the environment works after a switch, as the picker promises).
    pub fn resolve_key(&mut self) {
        self.api_key = self.api_keys.get(&self.provider).cloned().unwrap_or_default();
        self.key_from_env = false;
        if self.api_key.is_empty() {
            if let Some(k) = key_env_vars(&self.provider)
                .iter()
                .filter_map(|v| std::env::var(v).ok())
                .find(|k| !k.is_empty())
            {
                self.api_key = k;
                self.key_from_env = true;
            }
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

    pub fn apply_patch(&mut self, p: &ConfigPatch) {
        match p {
            ConfigPatch::Provider { provider, base_url, model } => {
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    #[test]
    fn server_addresses_reject_mistakes_before_saving_credentials() {
        for valid in ["http://localhost:1234/v1", "http://127.0.0.1:11434/v1", "https://gateway.example/api/v1", "http://[::1]:8080/v1"] {
            assert_eq!(super::normalize_provider_url(&format!(" {valid}/ ")).unwrap(), valid);
        }
        for invalid in ["", "localhost:1234", "https://", "https:host/v1", "http:/host/v1", "ftp://host/v1", "https://host:bad/v1",
            "https://host/v1 with space", "https://host/v1?key=secret", "https://host/v1#fragment",
            "https://user:secret@host/v1", "https://host/v1/chat/completions", "https://host/v1/models/"] {
            assert!(super::normalize_provider_url(invalid).is_err(), "{invalid}");
        }
    }

    use super::*;

    #[test]
    fn a_malformed_mcp_entry_is_skipped_but_survives_a_rewrite() {
        let mut cfg = Config::default();
        let v = serde_json::json!({
            "mcp_servers": {
                "good": {"command": "npx", "args": ["x"]},
                "bad": {"cmd": "typo"}
            }
        });
        // Mirror load_disk's per-entry logic on the parsed value.
        let m = &v["mcp_servers"];
        let mut broken = false;
        for (k, val) in m.as_object().unwrap() {
            match McpServerConfig::parse(val) {
                Some(c) => {
                    cfg.mcp_servers.insert(k.clone(), c);
                }
                None => broken = true,
            }
        }
        if broken {
            cfg.extra.insert("mcp_servers".into(), m.clone());
        }
        assert_eq!(cfg.mcp_servers.len(), 1, "the good server still loads");
        let out = cfg.to_disk_json();
        assert_eq!(out["mcp_servers"], v["mcp_servers"], "the raw block round-trips verbatim");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_the_targets_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("haikucode-aw-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&path, b"{\"a\":1}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewrite must not widen permissions");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_providers_round_trip_and_keep_their_model() {
        let mut c = Config::default();
        c.apply_patch(&ConfigPatch::AddProvider(CustomProvider {
            name: "gateway".into(),
            base_url: "https://gw.example/v1".into(),
            model: "m1".into(),
        }));
        c.apply_patch(&ConfigPatch::Provider {
            provider: "gateway".into(),
            base_url: "https://gw.example/v1".into(),
            model: "m1".into(),
        });
        c.apply_patch(&ConfigPatch::ApiKey("k".into()));
        // a model change while it's active is remembered in its entry
        c.model = "m2".into();
        c.sync_active_custom();
        c.apply_patch(&ConfigPatch::BaseUrl("https://gw2.example/v1".into()));
        let json = c.to_disk_json();
        let mut back = Config::default();
        back.absorb_disk(&json);
        assert_eq!(
            back.custom_provider("gateway"),
            Some(&CustomProvider { name: "gateway".into(), base_url: "https://gw2.example/v1".into(), model: "m2".into() })
        );
        assert_eq!(back.api_keys.get("gateway").map(String::as_str), Some("k"));
        let names: Vec<String> = back.provider_choices().into_iter().map(|(n, _, _)| n).collect();
        assert_eq!(names.last().map(String::as_str), Some("gateway"), "presets first, then custom");
        // removing forgets its key too
        back.apply_patch(&ConfigPatch::RemoveProvider("gateway".into()));
        assert!(back.custom_provider("gateway").is_none() && !back.api_keys.contains_key("gateway"));
    }

    #[test]
    fn a_wizard_custom_provider_joins_the_list_on_load() {
        // Written by the setup wizard before custom providers were a list.
        let v = serde_json::json!({
            "provider": "litellm",
            "base_url": "https://llm.example/v1",
            "model": "gemini-x",
            "api_keys": {"litellm": "k", "deepseek": "d"}
        });
        let mut c = Config::default();
        c.absorb_disk(&v);
        assert_eq!(c.custom_provider("litellm").map(|p| p.model.as_str()), Some("gemini-x"));
        // a preset never joins, and malformed entries are skipped
        let v = serde_json::json!({
            "provider": "deepseek",
            "custom_providers": [{"name": "openai", "base_url": "https://x", "model": "m"}, {"name": "ok", "base_url": "https://y", "model": "m"}, 3]
        });
        let mut c = Config::default();
        c.absorb_disk(&v);
        let names: Vec<&str> = c.custom_providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["ok"]);
    }

    #[test]
    fn patches_map_to_fields() {
        let mut c = Config::default();
        c.apply_patch(&ConfigPatch::Provider {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
        });
        assert_eq!((c.provider.as_str(), c.model.as_str()), ("openai", "gpt-4o-mini"));
        c.apply_patch(&ConfigPatch::Reasoning("High".into()));
        c.apply_patch(&ConfigPatch::Permission("plan".into()));
        c.apply_patch(&ConfigPatch::AutoCommit(false));
        c.apply_patch(&ConfigPatch::ContextWindow(64000));
        c.apply_patch(&ConfigPatch::MaxToolCalls(42));
        assert_eq!(c.reasoning, "high");
        assert_eq!(c.permission, "plan");
        assert!(!c.auto_commit);
        assert_eq!(c.context_window, 64000);
        assert_eq!(c.max_tool_calls, 42);
        // A key set through the panel always counts as a disk key.
        c.key_from_env = true;
        c.apply_patch(&ConfigPatch::ApiKey("sk-x".into()));
        assert!(!c.key_from_env);
        // Zero context window is clamped, not propagated.
        c.apply_patch(&ConfigPatch::ContextWindow(0));
        assert_eq!(c.context_window, 1);
        // Pinning the window through the panel marks it explicit so it sticks.
        assert!(c.context_window_explicit);
    }

    #[test]
    fn price_currency_is_normalized_and_persisted_only_when_set() {
        let mut c = Config::default();
        assert_eq!(
            c.price_currency, "USD",
            "prices are quoted in USD by default"
        );
        assert!(
            c.to_disk_json().get("price_currency").is_none(),
            "the default stays out of the file, like the other price fields"
        );

        c.apply_patch(&ConfigPatch::PriceCurrency("cny".into()));
        assert_eq!(c.price_currency, "CNY", "case is normalized on the way in");
        assert_eq!(c.to_disk_json()["price_currency"], "CNY");

        // A blank entry falls back to the documented default rather than
        // leaving the status line with no unit at all.
        c.apply_patch(&ConfigPatch::PriceCurrency("   ".into()));
        assert_eq!(c.price_currency, "USD");
        assert!(c.to_disk_json().get("price_currency").is_none());
    }

    #[test]
    fn a_hand_edited_price_currency_is_read_back() {
        let v: serde_json::Value = serde_json::from_str(r#"{"price_currency": "cny"}"#).unwrap();
        let code = v.get("price_currency").and_then(|x| x.as_str()).unwrap();
        assert_eq!(crate::money::Currency::parse(code).symbol, "¥");
    }

    /// The cost counter follows the provider: switching presets takes their
    /// built-in price lists with them; a custom server without a list has no
    /// known cost (and the top-level mirror zeros out).
    #[test]
    fn the_price_list_follows_the_active_provider() {
        let mut c = Config::default();
        let ds = c.price_list().unwrap();
        assert!((ds.price_in - 0.15).abs() < 1e-9 && (ds.price_out - 0.6).abs() < 1e-9);
        assert_eq!(ds.price_currency, "USD");

        c.apply_patch(&ConfigPatch::Provider {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
        });
        let o = c.price_list().unwrap();
        assert!((o.price_in - 0.15).abs() < 1e-9 && (o.price_out - 0.6).abs() < 1e-9);
        assert!((o.price_cached() - 0.075).abs() < 1e-9, "OpenAI's cache-read price");

        c.apply_patch(&ConfigPatch::Provider {
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            model: "claude-opus-5-5".into(),
        });
        let a = c.price_list().unwrap();
        assert!((a.price_in - 15.0).abs() < 1e-9 && (a.price_out - 75.0).abs() < 1e-9);
        // The top-level fields mirror the active provider's list.
        assert!((c.price_in - 15.0).abs() < 1e-9 && (c.price_out - 75.0).abs() < 1e-9);

        c.apply_patch(&ConfigPatch::Provider {
            provider: "mylocal".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen3".into(),
        });
        assert!(c.price_list().is_none(), "a custom server has no known price");
        assert_eq!(c.price_in, 0.0, "the mirror zeros out");

        c.apply_patch(&ConfigPatch::Provider {
            provider: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            model: "claude-opus-5-5".into(),
        });
        assert!((c.price_list().unwrap().price_in - 15.0).abs() < 1e-9, "switching back keeps the table's list");
    }

    /// A user-set price list wins over the built-in table and survives
    /// switching providers and back.
    #[test]
    fn a_user_price_list_overrides_the_table_and_survives_a_round_trip() {
        let mut c = Config::default();
        c.prices.insert(
            "deepseek".into(),
            PriceList {
                price_in: 1.0,
                price_out: 4.0,
                price_cached_in: None,
                price_currency: "CNY".into(),
            },
        );
        c.sync_prices();
        assert_eq!(c.price_list().unwrap().price_currency, "CNY");
        assert_eq!(c.price_currency, "CNY", "the mirror follows the user's list");

        c.apply_patch(&ConfigPatch::Provider {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
        });
        assert_eq!(c.price_list().unwrap().price_currency, "USD", "openai gets its own list");

        c.apply_patch(&ConfigPatch::Provider {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
        });
        let back = c.price_list().unwrap();
        assert!((back.price_in - 1.0).abs() < 1e-9 && back.price_currency == "CNY", "the CNY list comes back");
    }

    /// A pre-per-provider file carries one top-level price list; it belongs
    /// to the provider that was active when it was written, and the built-in
    /// cache-hit price fills in where the file has none.
    #[test]
    fn a_legacy_top_level_price_list_folds_into_the_active_provider() {
        let v = serde_json::json!({
            "provider": "deepseek",
            "price_in": 1.0,
            "price_out": 4.0,
            "price_currency": "cny",
        });
        let mut c = Config::default();
        c.absorb_disk(&v);
        c.sync_prices();
        let list = c.price_list().unwrap();
        assert!((list.price_in - 1.0).abs() < 1e-9 && list.price_currency == "CNY");
        assert!((list.price_cached() - DEEPSEEK_CACHE_HIT_PRICE).abs() < 1e-9, "the built-in cache-hit price carries over");
        // Switching to another preset and back keeps the folded list.
        c.apply_patch(&ConfigPatch::Provider {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
        });
        c.apply_patch(&ConfigPatch::Provider {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model: "deepseek-flash".into(),
        });
        assert!((c.price_list().unwrap().price_in - 1.0).abs() < 1e-9);
    }

    /// A custom server without a price list stays hidden across save/reload:
    /// the zeroed top-level mirror must not be written as a user price list.
    #[test]
    fn a_custom_provider_without_a_list_keeps_its_cost_hidden() {
        let mut c = Config::default();
        c.custom_providers.push(CustomProvider {
            name: "mylocal".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen3".into(),
        });
        c.apply_patch(&ConfigPatch::Provider {
            provider: "mylocal".into(),
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen3".into(),
        });
        assert!(c.price_list().is_none());
        let json = c.to_disk_json();
        assert!(json.get("price_in").is_none() && json.get("price_currency").is_none(), "{json}");

        let mut back = Config::default();
        back.absorb_disk(&json);
        back.sync_prices();
        assert!(back.price_list().is_none(), "still hidden after a round trip");
        assert_eq!(back.price_in, 0.0);
    }

    /// The old `thinking: true` did something only on DeepSeek/GLM. Elsewhere
    /// (here: a local Qwen behind Splash) it must stay a no-op, not become a
    /// request that switches reasoning on.
    #[test]
    fn legacy_thinking_flag_keeps_its_old_reach() {
        let load = |v: serde_json::Value| {
            let mut c = Config::default();
            c.absorb_disk(&v);
            c.reasoning
        };
        assert_eq!(load(serde_json::json!({"provider": "deepseek", "thinking": true})), "medium");
        assert_eq!(load(serde_json::json!({"provider": "zai", "thinking": true})), "medium");
        assert_eq!(load(serde_json::json!({"provider": "splash", "thinking": true})), default_reasoning());
        // An explicit setting always wins.
        assert_eq!(load(serde_json::json!({"provider": "splash", "thinking": true, "reasoning": "high"})), "high");
    }

    #[test]
    fn statusline_round_trips_and_stays_out_of_the_file_when_default() {
        let mut c = Config::default();
        assert!(c.to_disk_json().get("statusline").is_none());
        c.statusline = vec!["git".into(), "ctx".into()];
        let json = c.to_disk_json();
        assert_eq!(json["statusline"], serde_json::json!(["git", "ctx"]));
        let mut back = Config::default();
        back.absorb_disk(&json);
        assert_eq!(back.statusline, ["git", "ctx"]);
    }

    #[test]
    fn deepseek_flash_takes_images_but_pro_does_not() {
        let mut c = Config::default();
        assert_eq!(c.model, "deepseek-flash");
        assert!(c.supports_images());
        c.model = "deepseek-v4-pro".into();
        assert!(!c.supports_images());
        c.provider = "zai".into();
        c.model = "glm-5.2".into();
        assert!(!c.supports_images());
    }

    #[test]
    fn temperature_is_only_sent_when_configured_or_for_deepseek() {
        let mut c = Config::default();
        assert_eq!(c.provider, "deepseek");
        assert_eq!(c.temperature(), Some(0.2), "DeepSeek keeps its old 0.2");
        c.provider = "splash".into();
        assert_eq!(c.temperature(), None, "others get the model's own sampling");
        c.temperature = Some(0.7);
        assert_eq!(c.temperature(), Some(0.7));
    }

    #[test]
    fn per_model_context_windows() {
        let v = serde_json::json!({
            "provider": "deepseek",
            "model": "Gemma4-26B-A4B-QAT",
            "context_window": 64000,
            "models": {
                "Gemma4-26B-A4B-QAT": {"context_window": 131072},
                "bad": {"context_window": "x"},
                "zero": {"context_window": 0}
            },
            // legacy map: folded in, never overriding a `models` value
            "model_context_windows": {"gemma4-coding-Q8_0": 8192, "Gemma4-26B-A4B-QAT": 1}
        });
        let mut c = Config::default();
        c.absorb_disk(&v);
        // the model's entry beats the global window; bad/zero entries are dropped
        assert_eq!(c.model_context_window(), Some(131072));
        assert_eq!(c.effective_context_window(), 131072);
        assert_eq!(c.models.len(), 2, "{:?}", c.models);
        // case-insensitive match
        c.model = "GEMMA4-CODING-q8_0".into();
        assert_eq!(c.effective_context_window(), 8192);
        // no entry: the global window
        c.model = "deepseek-flash".into();
        assert_eq!(c.model_context_window(), None);
        assert_eq!(c.effective_context_window(), 64000);
        // /config edits the global window for a model without an entry...
        c.apply_patch(&ConfigPatch::ContextWindow(100_000));
        assert_eq!((c.context_window, c.effective_context_window()), (100_000, 100_000));
        // ...and the entry (under its stored spelling) for one with an entry
        c.model = "gemma4-26b-a4b-qat".into();
        c.apply_patch(&ConfigPatch::ContextWindow(65536));
        assert_eq!(c.models["Gemma4-26B-A4B-QAT"].context_window, Some(65536));
        assert_eq!(c.context_window, 100_000);
        // round-trips through the file as `models`; the legacy key isn't rewritten
        let json = c.to_disk_json();
        assert_eq!(json["models"]["Gemma4-26B-A4B-QAT"]["context_window"], 65536);
        assert!(json.get("model_context_windows").is_none());
        let mut back = Config::default();
        back.absorb_disk(&json);
        assert_eq!(back.models, c.models);
        assert!(Config::default().to_disk_json().get("models").is_none());
    }

    #[test]
    fn per_model_temperature_and_prices_follow_the_model() {
        let v = serde_json::json!({
            "provider": "anthropic",
            "model": "claude-haiku-4-5",
            "temperature": 0.5,
            "models": {
                "claude-haiku-4-5": {"price_in": 1.0, "price_out": 5.0, "temperature": 0.9},
                "local-gemma": {"price_in": 0.0, "price_out": 0.0}
            }
        });
        let mut c = Config::default();
        c.absorb_disk(&v);
        c.sync_prices();
        assert_eq!(c.temperature(), Some(0.9));
        let p = c.effective_price_list().unwrap();
        assert_eq!((p.price_in, p.price_out), (1.0, 5.0));
        assert_eq!(p.price_cached_in, built_in_prices("anthropic").unwrap().price_cached_in, "unset fields come from the provider");
        assert_eq!(c.price_in, 1.0, "the mirror fields follow the model");
        // the provider's own list is untouched, and that's what the file mirrors
        assert_eq!(c.price_list().unwrap().price_in, 15.0);
        let json = c.to_disk_json();
        assert_eq!(json["price_in"], 15.0, "{json}");
        let mut back = Config::default();
        back.absorb_disk(&json);
        assert_eq!(back.price_list().unwrap().price_in, 15.0, "a model's price never leaks into the provider's list");
        // another model on the same provider: provider prices, global temperature
        c.model = "claude-opus".into();
        c.sync_prices();
        assert_eq!((c.price_in, c.temperature()), (15.0, Some(0.5)));
        // a custom server without a list still gets a cost readout for a priced model
        c.provider = "box".into();
        c.model = "local-gemma".into();
        assert_eq!(c.price_list(), None);
        assert_eq!(c.effective_price_list().map(|p| p.price_in), Some(0.0));
        c.model = "other".into();
        assert_eq!(c.effective_price_list(), None);
    }

    #[test]
    fn the_wire_protocol_follows_the_provider() {
        // legacy top-level `api` belongs to the provider active when written
        let v = serde_json::json!({"provider": "vercel", "model": "m", "api": "messages"});
        let mut c = Config::default();
        c.absorb_disk(&v);
        assert!(matches!(c.api_kind(), ApiKind::Messages));
        assert_eq!(c.apis.get("vercel").map(String::as_str), Some("messages"));
        c.provider = "box".into();
        assert!(matches!(c.api_kind(), ApiKind::Chat), "a local server doesn't inherit the gateway's protocol");
        c.provider = "anthropic".into();
        assert!(matches!(c.api_kind(), ApiKind::Messages));
        let json = c.to_disk_json();
        assert!(json.get("api").is_none());
        assert_eq!(json["apis"]["vercel"], "messages");
    }

    #[test]
    fn context_window_table_by_model() {
        assert_eq!(known_context_window("deepseek-v4-pro"), 1_000_000);
        assert_eq!(known_context_window("deepseek-v4-flash"), 1_000_000);
        // `deepseek-flash` is the API's own alias for v4 Flash — also 1M.
        assert_eq!(known_context_window("deepseek-flash"), 1_000_000);
        assert_eq!(known_context_window("deepseek-chat"), 128_000);
        assert_eq!(known_context_window("deepseek-reasoner"), 128_000);
        // A non-DeepSeek "flash" (GLM-4-Flash) must not inherit the 1M window.
        assert_eq!(known_context_window("glm-4-flash"), 128_000);
        // Unknown models fall back to the conservative 128k, not the 1M default.
        assert_eq!(known_context_window("gpt-4o-mini"), 128_000);
        // A fresh config auto-derives, and stays non-explicit so it keeps
        // tracking the model and never gets written to disk.
        let c = Config::default();
        assert!(!c.context_window_explicit);
    }

    #[test]
    fn env_keys_are_provider_scoped() {
        // Only the configured provider's var (plus the universal override)
        // can supply the key — never another provider's.
        for (name, _, _) in PROVIDERS {
            let vars = key_env_vars(name);
            assert_eq!(vars[0], "HAIKUCODE_API_KEY", "{name}: override must win");
            for v in &vars[1..] {
                let prov_upper = name.to_uppercase();
                assert!(
                    v.starts_with(&prov_upper)
                        || (*name == "google" && v.starts_with("GEMINI"))
                        || (*name == "zai" && (v.starts_with("ZAI") || v.starts_with("ZHIPUAI")))
                        || (*name == "vercel" && *v == "AI_GATEWAY_API_KEY"),
                    "{name} must not read {v}"
                );
            }
        }
        // Unknown/custom providers get only the explicit override.
        assert_eq!(key_env_vars("my-llm-box"), &["HAIKUCODE_API_KEY"]);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("haikucode_atomic_test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, b"old contents").unwrap();
        atomic_write(&path, b"new contents").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new contents");
        // The per-process temp file must not survive a successful write.
        let leftover = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".tmp."));
        assert!(!leftover, "atomic_write left a temp file behind");
        // Writing to a brand-new path works too.
        let fresh = dir.join("fresh.json");
        atomic_write(&fresh, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "{}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// picoder's directory is copied over once, without its `run/` sockets,
    /// and never over an existing Haiku Code setup.
    #[test]
    fn picoder_settings_are_adopted_once() {
        let root = std::env::temp_dir().join(format!("hc-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (old, new) = (root.join("picoder"), root.join("haikucode"));
        std::fs::create_dir_all(old.join("sessions/abc")).unwrap();
        std::fs::create_dir_all(old.join("run")).unwrap();
        std::fs::write(old.join("config.json"), "{}").unwrap();
        std::fs::write(old.join("sessions/abc/1.json"), "[]").unwrap();
        std::fs::write(old.join("run/sock"), "").unwrap();
        assert!(adopt_picoder_from(&old, &new).unwrap().starts_with("imported"));
        assert!(new.join("config.json").is_file());
        assert!(new.join("sessions/abc/1.json").is_file());
        assert!(!new.join("run").exists());
        assert!(adopt_picoder_from(&old, &new).is_none(), "second run copies nothing");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(unix)]
fn set_private(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private(_path: &std::path::Path) {}

/// Interactive first-run setup, executed before the TUI starts (cooked stdin):
/// the same three steps as `/provider` — provider, sign-in, model.
pub fn run_setup() -> Result<Config> {
    let mut cfg = Config::load();
    let bold = |t: &str| println!("\n\x1b[1m{t}\x1b[0m");
    let dim = |t: &str| println!("\x1b[2m  {t}\x1b[0m");
    let err = |t: &str| println!("\x1b[31m  {t}\x1b[0m");
    println!("\x1b[1mhaikucode setup\x1b[0m  \x1b[2m(Enter takes the [default]; Ctrl+C quits)\x1b[0m");

    // Step 1: provider.
    bold("Step 1 of 3 · Provider");
    let width = PROVIDERS.iter().map(|(n, _, _)| n.len()).max().unwrap_or(8);
    let current = PROVIDERS.iter().position(|(n, _, _)| *n == cfg.provider).unwrap_or(0);
    for (i, (name, base, _)) in PROVIDERS.iter().enumerate() {
        let ways = auth_methods(name)
            .iter()
            .map(|m| method_short(name, *m))
            .collect::<Vec<_>>()
            .join(" or ");
        let host = base.split('/').nth(2).unwrap_or(base);
        println!("  {:>2}) {name:<width$}  \x1b[2m{host} · {ways}\x1b[0m", i + 1);
    }
    let custom_n = PROVIDERS.len() + 1;
    println!("  {custom_n:>2}) {:<width$}  \x1b[2many OpenAI-compatible URL (LiteLLM, Ollama, LM Studio, …)\x1b[0m", "custom");
    let n = loop {
        let choice = prompt(&format!("> [{}] ", current + 1))?;
        if choice.is_empty() {
            break current + 1;
        }
        match choice.parse::<usize>() {
            Ok(n) if (1..=custom_n).contains(&n) => break n,
            _ => match PROVIDERS.iter().position(|(p, _, _)| *p == choice) {
                Some(i) => break i + 1,
                None => err(&format!("pick 1 to {custom_n}, or type a provider's name")),
            },
        }
    };
    let custom = n == custom_n;
    let (prov, base, default_model): (String, String, String) = if custom {
        let p = loop {
            let p = prompt("  name [custom]: ")?;
            let p = nonempty(p, "custom");
            match valid_custom_name(&p) {
                Ok(()) => break p,
                Err(e) => err(&e),
            }
        };
        let saved = cfg.custom_provider(&p).cloned();
        let b = loop {
            let hint = saved.as_ref().map(|c| format!(" [{}]", c.base_url)).unwrap_or_default();
            dim("the OpenAI-compatible endpoint, e.g. http://localhost:11434/v1");
            let b = prompt(&format!("  base URL{hint}: "))?;
            let b = nonempty(b, saved.as_ref().map(|c| c.base_url.as_str()).unwrap_or(""));
            match normalize_provider_url(&b) {
                Ok(b) => break b,
                Err(e) => err(&e),
            }
        };
        (p, b, saved.map(|c| c.model).unwrap_or_default())
    } else {
        let (p, b, m) = PROVIDERS[n - 1];
        (p.into(), b.into(), m.into())
    };
    cfg.provider = prov.clone();
    cfg.base_url = base.clone();
    cfg.resolve_key();

    // Step 2: sign in.
    bold(&format!("Step 2 of 3 · Sign in to {prov}"));
    let methods = auth_methods(&prov);
    if let Some(why) = no_sub_reason(&prov) {
        dim(why);
    }
    let method = if methods.len() == 1 {
        methods[0]
    } else {
        let cur = methods.iter().position(|m| *m == cfg.auth_for(&prov)).unwrap_or(0);
        for (i, m) in methods.iter().enumerate() {
            println!("  {}) {}", i + 1, method_long(&prov, *m, &cfg));
        }
        loop {
            let c = prompt(&format!("> [{}] ", cur + 1))?;
            if c.is_empty() {
                break methods[cur];
            }
            match c.parse::<usize>() {
                Ok(i) if (1..=methods.len()).contains(&i) => break methods[i - 1],
                _ => err(&format!("pick 1 to {}", methods.len())),
            }
        }
    };
    if method == AuthMethod::Api {
        cfg.auth_modes.remove(&prov);
    } else {
        cfg.auth_modes.insert(prov.clone(), method.id().to_string());
    }
    cfg.sync_auth_mode();
    match method {
        AuthMethod::Api | AuthMethod::Plan => {
            if let Some(url) = key_url(&prov, method == AuthMethod::Plan) {
                dim(&format!("get one at {url}"));
            }
            let saved = cfg.api_keys.get(&prov).filter(|k| !k.is_empty()).map(|k| key_tail(k));
            let label = if method == AuthMethod::Plan { coding_plan_label(&prov) } else { "API" };
            let key = loop {
                let hint = match &saved {
                    Some(t) => format!(" [saved …{t}, Enter keeps it]"),
                    None if custom => " [none]".into(),
                    None => String::new(),
                };
                let k = prompt_secret(&format!("  {label} key{hint} (hidden): "))?;
                if !k.is_empty() || saved.is_some() || custom {
                    break k;
                }
                err("paste a key (or Ctrl+C and pick another provider)");
            };
            if !key.is_empty() {
                cfg.api_keys.insert(prov.clone(), key.clone());
                cfg.api_key = key;
                cfg.key_from_env = false;
            }
        }
        AuthMethod::Sub => {
            let p = crate::auth::provider(&prov).ok_or_else(|| anyhow::anyhow!("{prov} has no subscription sign-in"))?;
            let login = crate::auth::start(p)?;
            println!("  Opening your browser to sign in with {}.", crate::auth::sub_label(&prov));
            dim(&format!("if it didn't open, visit: {}", login.url));
            let token = login.finish()?;
            cfg.oauth.insert(prov.clone(), token);
            println!("\x1b[32m  signed in\x1b[0m");
        }
        AuthMethod::None => dim("no key: requests go out without an Authorization header"),
    }

    // Step 3: model, from the server's list when it answers.
    bold("Step 3 of 3 · Model");
    let default_model = if cfg.chatgpt_sub() { String::new() } else { default_model };
    let ids = match crate::api::list_models(&crate::api::agent_http(), &cfg) {
        Ok(ids) => crate::api::chat_models(ids),
        Err(e) => {
            err(&format!("couldn't load the model list: {}", crate::api::truncate(&e.to_string(), 200)));
            if matches!(method, AuthMethod::Api | AuthMethod::Plan) && e.to_string().contains("HTTP 401") {
                dim("the key was refused: run `haikucode --config` again to re-enter it");
            }
            Vec::new()
        }
    };
    let default_model = if default_model.is_empty() || (!ids.is_empty() && !ids.contains(&default_model)) {
        ids.first().cloned().unwrap_or(default_model)
    } else {
        default_model
    };
    const SHOWN: usize = 30;
    for (i, id) in ids.iter().take(SHOWN).enumerate() {
        let mark = if *id == default_model { "  (default)" } else { "" };
        println!("  {:>2}) {id}{mark}", i + 1);
    }
    if ids.len() > SHOWN {
        dim(&format!("…and {} more: type any model id", ids.len() - SHOWN));
    }
    let model = loop {
        let hint = if default_model.is_empty() { String::new() } else { format!(" [{default_model}]") };
        let ask = if ids.is_empty() { "  model id" } else { "  number or model id" };
        let m = prompt(&format!("{ask}{hint}: "))?;
        let m = match m.parse::<usize>() {
            Ok(i) if (1..=ids.len().min(SHOWN)).contains(&i) => ids[i - 1].clone(),
            _ => nonempty(m, &default_model),
        };
        if !m.is_empty() {
            break m;
        }
        err("a model id is required");
    };
    cfg.model = model;
    if custom {
        cfg.custom_providers.retain(|c| c.name != cfg.provider);
        cfg.custom_providers.push(CustomProvider {
            name: cfg.provider.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
        });
    }
    if !cfg.context_window_explicit {
        cfg.context_window = known_context_window(&cfg.model);
    }
    cfg.sync_prices();
    cfg.save()?;
    println!(
        "\n\x1b[32m✓ {} · {} · {}\x1b[0m  \x1b[2msaved {}\x1b[0m",
        cfg.provider,
        method_short(&cfg.provider, method),
        cfg.model,
        config_path().display()
    );
    Ok(cfg)
}

/// A sign-in method in a few words, e.g. "API key" or "ChatGPT sign-in".
pub fn method_short(provider: &str, m: AuthMethod) -> String {
    match m {
        AuthMethod::Api => "API key".into(),
        AuthMethod::Sub => format!("{} sign-in", crate::auth::sub_label(provider)),
        AuthMethod::Plan => coding_plan_label(provider).into(),
        AuthMethod::None => "no key".into(),
    }
}

/// A sign-in method as a menu line, with what's already saved for it.
pub fn method_long(provider: &str, m: AuthMethod, cfg: &Config) -> String {
    let saved_key = cfg.api_keys.get(provider).filter(|k| !k.is_empty()).map(|k| key_tail(k));
    match m {
        AuthMethod::Api => match saved_key {
            Some(t) if cfg.auth_for(provider) == AuthMethod::Api => format!("API key — saved …{t}"),
            _ => "API key — pay as you go".into(),
        },
        AuthMethod::Sub if cfg.signed_in(provider) => {
            format!("{} subscription — signed in", crate::auth::sub_label(provider))
        }
        AuthMethod::Sub => match provider {
            "openai" => "ChatGPT subscription (Plus / Pro / Business) — sign in in your browser".into(),
            _ => "subscription — sign in in your browser".into(),
        },
        AuthMethod::Plan => match saved_key {
            Some(t) if cfg.auth_for(provider) == AuthMethod::Plan => {
                format!("{} — saved key …{t}", coding_plan_label(provider))
            }
            _ => format!("{} — a subscription key, on the plan's own endpoint", coding_plan_label(provider)),
        },
        AuthMethod::None => "no key — a local or open server".into(),
    }
}

/// The last four characters of a key, for "saved …abcd".
pub fn key_tail(k: &str) -> String {
    let n = k.chars().count();
    k.chars().skip(n.saturating_sub(4)).collect()
}

/// `prompt` without echoing what's typed (`stty -echo`), for keys.
fn prompt_secret(msg: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let stty = |arg: &str| {
        Command::new("stty")
            .arg(arg)
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let hidden = stty("-echo");
    let line = prompt(msg);
    if hidden {
        stty("echo");
        println!();
    }
    line
}

fn prompt(msg: &str) -> Result<String> {
    print!("{msg}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn nonempty(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}
