//! Configuration: provider/base_url/model/api_key, loaded from
//! ~/config/settings/haikucode/config.json and overridable by env. Compatible with the
//! previous Python haikucode config so existing keys/memory carry over.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

mod credentials;
mod disk;
mod models;
mod patch;
mod paths;
mod providers;
mod setup;
#[cfg(test)]
mod tests_disk;
#[cfg(test)]
mod tests_models;

pub use patch::ConfigPatch;
pub use paths::{adopt_picoder, atomic_write, config_dir, config_dir_label, config_path, history_path, memory_path, sessions_dir, setting, settings_layers};
pub use providers::{
    auth_methods, built_in_prices, coding_plan_label, coding_plan_url, is_builtin_provider, key_url, known_context_window, no_sub_reason,
    normalize_provider_url, valid_custom_name, AuthMethod, CHATGPT_CODEX_URL, PROVIDERS,
};
use providers::key_env_vars;
pub use setup::{key_tail, method_short, run_setup};

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
    /// What each built-in provider was last used with (`presets` in
    /// config.json): its model, and its base URL when that isn't the
    /// preset's (a proxy). Custom providers keep theirs in
    /// `custom_providers`; this gives the presets the same memory, so
    /// switching away and back doesn't reset them.
    #[serde(default)]
    pub presets: BTreeMap<String, PresetState>,
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

/// A built-in provider's remembered state (see `Config::presets`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
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

fn default_theme() -> String {
    "HaikuOS".to_string()
}
fn default_ctx() -> u32 {
    // Matches the default model, DeepSeek v4 Flash (1M-token context window).
    1_000_000
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
            presets: BTreeMap::new(),
            prices: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}
